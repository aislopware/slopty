// slopty: Slopty's own file (not in zeron).
//! slopty: what a request's card adds to zeron's question panel, by the request's kind: who asks
//! when a chat an agent started under this one does ("Sent by reviewer", answered here), when
//! the agent stops waiting, what each option means, "Allow with rule" with the rule and its
//! scope editable, "Revise…" on a plan (and "Say what to do instead…" on an approval) to send
//! words instead of a choice, "Allow, and say what's next…" to send them with the allow, "Edit
//! plan…" to approve a plan as edited, "Chat about this" beside an agent's questions, the
//! options' previews in a pane beside them, an MCP server's form drawn from its schema (or the page to fill it
//! in at), and a dialog of the agent's own shown from its payload, a kind Slopty does not know
//! included, never refused unseen (`docs/decisions/ui.md`).

use std::time::{Duration, Instant};

use gpui::AnyElement;
use serde_json::Value;
use slopty_view_doc::calls;
use slopty_view_doc::rules::{PermissionRules, RuleScope};
use slopty_view_doc::work::duration_words;

use super::*;

/// The request kinds the card draws its own way (`slopty_proto::thread::Request`).
pub(crate) const ELICITATION: &str = "elicitation";
const DIALOG: &str = "dialog:";
const PLAN: &str = "plan";
const APPROVAL: &str = "approval";
/// The answer that dismisses a dialog or a form without one.
const CANCEL: &str = "cancel";
/// The id an answer of the choice beside an agent's questions goes under (`aside`).
const ASIDE_ID: &str = "choice";
/// The id an answer that skips an agent's questions goes under: the view model sends it as the
/// request's plain decline (`slopty_ui::view_model::transcript::SKIP`).
pub(crate) const SKIP_ID: &str = "skip";
/// The kind of a request that asks an agent's questions.
pub(crate) const QUESTION: &str = "question";
/// Where a question's notes box is kept among the card's inputs, before the question's id.
const NOTES: &str = "notes:";
/// The most of a dialog's payload the card lists.
const MAX_FACTS: usize = 8;
/// What a card only the person at it answers says (`slopty_proto::thread::Request::NEEDS_PRESENCE`).
pub(crate) const NEEDS_PRESENCE: &str = "Answer here";
/// Why, in its tooltip and for a screen reader (`Request::NEEDS_PRESENCE_REFUSED`'s words).
pub(crate) const NEEDS_PRESENCE_WHY: &str = "The agent takes this answer only from its card in Slopty";
/// Under this long left, the countdown turns amber.
const COUNTDOWN_URGENT_MS: i64 = 30_000;

/// What a form's field takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Text,
    Number { integer: bool },
    Bool,
    /// One of these `(value, label)`s.
    Choice(Vec<(String, String)>),
    /// Any of these `(value, label)`s.
    Choices(Vec<(String, String)>),
}

/// One field of an MCP server's form.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FormField {
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    pub kind: FieldKind,
    pub required: bool,
    /// Its default, as the field shows it.
    pub default: Option<String>,
    /// What its value must be, as the schema says.
    pub limits: Limits,
}

/// A field's limits, from its schema: lengths, an email or address format, a range.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Limits {
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    pub format: Option<String>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

impl Limits {
    fn of(field: &Value) -> Self {
        Self {
            min_length: field.get("minLength").and_then(Value::as_u64),
            max_length: field.get("maxLength").and_then(Value::as_u64),
            format: field.get("format").and_then(Value::as_str).map(str::to_owned),
            minimum: field.get("minimum").and_then(Value::as_f64),
            maximum: field.get("maximum").and_then(Value::as_f64),
        }
    }

    /// Why `text` breaks a limit of field `title`, as the TUI says it.
    fn text_fault(&self, title: &str, text: &str) -> Option<String> {
        let chars = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
        if let Some(min) = self.min_length.filter(|min| chars < *min) {
            return Some(format!("{title} must be at least {min} characters"));
        }
        if let Some(max) = self.max_length.filter(|max| chars > *max) {
            return Some(format!("{title} must be at most {max} characters"));
        }
        match self.format.as_deref() {
            Some("email") if !looks_like_email(text) => {
                Some(format!("{title} must be a valid email address, e.g. user@example.com"))
            }
            Some("uri") if url::Url::parse(text).is_err() => {
                Some(format!("{title} must be a full address, e.g. https://example.com"))
            }
            _ => None,
        }
    }

    /// Why `n` is out of range for field `title`.
    fn number_fault(&self, title: &str, n: f64) -> Option<String> {
        if let Some(min) = self.minimum.filter(|min| n < *min) {
            return Some(format!("{title} must be at least {min}"));
        }
        self.maximum.filter(|max| n > *max).map(|max| format!("{title} must be at most {max}"))
    }
}

fn looks_like_email(text: &str) -> bool {
    text.split_once('@').is_some_and(|(user, host)| {
        !user.is_empty() && host.contains('.') && !host.starts_with('.') && !host.ends_with('.')
    }) && !text.contains(char::is_whitespace)
}

/// The `(value, label)`s an enum-like schema offers: `enum` with `enumNames`, or `oneOf` /
/// `anyOf` of `{ const, title }`.
fn offered(schema: &Value) -> Option<Vec<(String, String)>> {
    let word = |v: &Value| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string());
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let names = schema.get("enumNames").and_then(Value::as_array);
        return Some(
            values
                .iter()
                .enumerate()
                .map(|(ix, v)| {
                    let label = names.and_then(|n| n.get(ix)).map_or_else(|| word(v), word);
                    (word(v), label)
                })
                .collect(),
        );
    }
    let list = schema.get("oneOf").or_else(|| schema.get("anyOf"))?.as_array()?;
    let pairs: Vec<_> = list
        .iter()
        .filter_map(|o| {
            let value = word(o.get("const")?);
            let label = o.get("title").map_or_else(|| value.clone(), word);
            Some((value, label))
        })
        .collect();
    (!pairs.is_empty()).then_some(pairs)
}

/// The fields of a form's JSON Schema, in the server's order (`x-property-order`, else the
/// schema's); a schema that is no object of properties has none.
pub(crate) fn form_fields(schema_json: &str) -> Vec<FormField> {
    let Ok(schema) = serde_json::from_str::<Value>(schema_json) else { return Vec::new() };
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut names: Vec<String> = schema
        .get("x-property-order")
        .and_then(Value::as_array)
        .map(|o| o.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    names.retain(|name| properties.contains_key(name));
    for name in properties.keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
        .into_iter()
        .filter_map(|name| {
            let field = properties.get(&name)?;
            let kind = match field.get("type").and_then(Value::as_str) {
                Some("boolean") => FieldKind::Bool,
                Some("number") => FieldKind::Number { integer: false },
                Some("integer") => FieldKind::Number { integer: true },
                Some("array") => FieldKind::Choices(field.get("items").and_then(offered)?),
                _ => offered(field).map_or(FieldKind::Text, FieldKind::Choice),
            };
            let default = field.get("default").map(|d| match d {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
            Some(FormField {
                title: field.get("title").and_then(Value::as_str).unwrap_or(&name).to_owned(),
                description: field.get("description").and_then(Value::as_str).map(str::to_owned),
                kind,
                required: required.contains(&name.as_str()),
                default,
                limits: Limits::of(field),
                name,
            })
        })
        .collect()
}

/// The form's answer: a JSON object of what `texts` (text and number fields) and `picks`
/// (choices, and `"true"`/`"false"` for a yes/no) hold; why not, when a required field is empty
/// or a number is not one.
pub(crate) fn form_value(
    fields: &[FormField],
    texts: &HashMap<String, String>,
    picks: &HashMap<String, Vec<String>>,
) -> Result<Value, String> {
    let mut object = serde_json::Map::new();
    for field in fields {
        let picked = picks.get(&field.name).filter(|p| !p.is_empty());
        let text = texts.get(&field.name).map(|t| t.trim()).filter(|t| !t.is_empty());
        let value = match &field.kind {
            FieldKind::Text => match text {
                Some(t) => match field.limits.text_fault(&field.title, t) {
                    Some(fault) => return Err(fault),
                    None => Some(Value::String(t.to_owned())),
                },
                None => None,
            },
            FieldKind::Number { integer } => match text {
                None => None,
                Some(t) => {
                    let n = if *integer {
                        t.parse::<i64>().ok().map(|n| (Value::from(n), t.parse::<f64>().unwrap_or_default()))
                    } else {
                        t.parse::<f64>().ok().and_then(|n| Some((Value::Number(serde_json::Number::from_f64(n)?), n)))
                    };
                    let Some((value, n)) = n else {
                        let kind = if *integer { "a whole number" } else { "a number" };
                        return Err(format!("{} takes {kind}", field.title));
                    };
                    if let Some(fault) = field.limits.number_fault(&field.title, n) {
                        return Err(fault);
                    }
                    Some(value)
                }
            },
            FieldKind::Bool => picked.and_then(|p| p.first()).map(|p| Value::Bool(p == "true")),
            FieldKind::Choice(_) => picked.and_then(|p| p.first()).cloned().map(Value::String),
            FieldKind::Choices(_) => {
                picked.map(|p| Value::Array(p.iter().cloned().map(Value::String).collect()))
            }
        };
        match value {
            Some(value) => {
                object.insert(field.name.clone(), value);
            }
            None if field.required => return Err(format!("Fill in {}", field.title)),
            None => {}
        }
    }
    Ok(Value::Object(object))
}

/// A key as people read it: `fallbackModel` and `fallback_model` both "Fallback model".
fn key_words(key: &str) -> String {
    let mut words = String::new();
    for (ix, c) in key.chars().enumerate() {
        if c == '_' || c == '-' {
            words.push(' ');
        } else if c.is_uppercase() && ix > 0 {
            words.push(' ');
            words.extend(c.to_lowercase());
        } else if ix == 0 {
            words.extend(c.to_uppercase());
        } else {
            words.push(c);
        }
    }
    words
}

/// What a dialog's payload says beyond what its card already shows (`shown`): its plain
/// facts, by name, at most [`MAX_FACTS`].
pub(crate) fn payload_facts(payload_json: &str, shown: &str) -> Vec<(String, String)> {
    let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(payload_json) else {
        return Vec::new();
    };
    payload
        .iter()
        .filter(|(key, _)| key.as_str() != "options")
        .filter_map(|(key, value)| {
            let words = match value {
                Value::String(s) if !s.trim().is_empty() => s.trim().to_owned(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => (if *b { "Yes" } else { "No" }).to_owned(),
                Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_string) => {
                    items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
                }
                _ => return None,
            };
            (!shown.contains(&words)).then(|| (key_words(key), words))
        })
        .take(MAX_FACTS)
        .collect()
}

/// When the agent stops waiting, in words, at `now`: "Declines by itself in 42s".
pub(crate) fn countdown_words(until_ms: i64, now: i64) -> Option<String> {
    let left = until_ms.checked_sub(now).filter(|left| *left > 0)?;
    let left = u64::try_from(left).ok()?.div_ceil(1_000) * 1_000;
    Some(format!("Declines by itself in {}", duration_words(left)))
}

/// Whether a request's reason is the auto-mode classifier's escalation, which the TUI draws in
/// its error colour ("Auto mode classifier requires confirmation…", "Classifier {name}…").
pub(crate) fn classifier_escalated(reason: &str) -> bool {
    let reason = reason.trim_start();
    reason.starts_with("Auto mode classifier") || reason.starts_with("Classifier ")
}

/// A scope's words when the agent has not said its own: Claude Code's three.
pub(crate) fn scope_words(id: &str) -> String {
    match id {
        "local" => "This project, this machine".to_owned(),
        "project" => "This project".to_owned(),
        "user" => "All projects".to_owned(),
        other => key_words(other),
    }
}

/// A dialog of the agent's that offers no answers of its own: it takes OK or Dismiss, never
/// words.
pub(crate) fn takes_no_words(question: &UserInputQuestion) -> bool {
    question.kind.starts_with(DIALOG) && question.options.is_empty()
}

/// Whether `question` is a form: an MCP server's request with fields to fill in. Its answers
/// (Decline, Cancel) are buttons beside its Submit, never numbered options, and no key picks
/// one.
pub(crate) fn is_form(question: &UserInputQuestion) -> bool {
    question.kind == ELICITATION && question.schema_json.as_deref().is_some_and(|schema| !form_fields(schema).is_empty())
}

/// Whether the card shows the composer's input as its answer box: a request that takes words,
/// or a choice being revised in words.
pub(crate) fn takes_words(question: &UserInputQuestion, revising: bool) -> bool {
    (!question.choices_only || revising) && !takes_no_words(question)
}

/// The longest a URL's browser-ready form may be for the card to open it (the TUI's `iZe`).
const URL_BROWSER_CHARS: usize = 8_000;

/// Why a form's URL is not opened with a click, in the TUI's words: it holds what would hide or
/// reorder what the person reads, or its browser-ready form (each shell metacharacter escaped)
/// is too long to hand over safely.
pub(crate) fn url_caution(url: &str) -> Option<&'static str> {
    // slopty: the view model shows each hidden character as its code (`[U+202E]`), which no
    // address can hold as it stands.
    if url.contains("[U+") || url.chars().any(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}' | '\u{2800}')) {
        return Some("This URL cannot be shown exactly as it would open, so opening it is disabled. Decline to continue.");
    }
    let escaped = url.chars().map(|c| match c {
        '"' => 8,
        '(' | ')' | '[' | ']' | '%' | '!' | '^' | '`' | '<' | '>' | '&' | '|' | ';' | ',' | ' ' | '*' | '?' => 4,
        '\\' => 2,
        _ => 1,
    });
    let browser = escaped.fold(16_usize, usize::saturating_add);
    (browser > URL_BROWSER_CHARS).then_some(
        "This URL’s browser-ready form is too long to hand to a browser safely, so one-click opening is disabled. The URL above is shown in full. Decline to continue.",
    )
}

/// What "Revise…" says on `question`, when it offers it: a plan, or an approval that has a
/// plain deny to carry the words.
pub(crate) fn revise_words(question: &UserInputQuestion, plan: bool) -> Option<&'static str> {
    if !question.choices_only || question.deny_option.is_none() {
        return None;
    }
    if plan || question.kind == PLAN {
        Some("Revise…")
    } else if question.kind == APPROVAL || question.kind.is_empty() {
        Some("Say what to do instead…")
    } else {
        None
    }
}

/// What "Allow, and say what's next…" says on `question`, when it offers it: an approval whose
/// agent takes words with an allow (the TUI's "Yes, and tell Claude what to do next").
pub(crate) fn allow_words(question: &UserInputQuestion) -> Option<&'static str> {
    (question.choices_only && question.kind == APPROVAL && question.allow_option.is_some())
        .then_some("Allow, and say what's next…")
}

/// How long after the last keystroke, and after a card appears, its option keys wait: a key
/// the person meant for the composer never answers a request that came as they typed
/// (t3code's `COMPOSER_TYPING_IDLE_MS`).
pub(crate) const KEYS_IDLE: Duration = Duration::from_millis(1_500);

/// When the person last typed and when the card last appeared, which hold its option keys
/// back for [`KEYS_IDLE`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct KeyGuard {
    pub(crate) typed_at: Option<Instant>,
    pub(crate) shown_at: Option<Instant>,
}

impl KeyGuard {
    /// Whether an option key at `now` is held back.
    pub(crate) fn holds(&self, now: Instant) -> bool {
        [self.typed_at, self.shown_at]
            .into_iter()
            .flatten()
            .any(|at| now.saturating_duration_since(at) < KEYS_IDLE)
    }
}

/// An approval's "Allow with rule" while it is edited.
pub(crate) struct RuleEdit {
    input: Entity<ComposerInput>,
    scope: String,
}

/// What a request's card keeps between frames, for the request it was made for.
#[derive(Default)]
pub(crate) struct CardState {
    request: Option<String>,
    rule: Option<RuleEdit>,
    /// The scopes the agent says a rule can go to (`slopty.WatchRules`), once said.
    scopes: Vec<RuleScope>,
    scopes_watch: Option<Task<()>>,
    inputs: HashMap<String, Entity<ComposerInput>>,
    picks: HashMap<String, Vec<String>>,
    error: Option<String>,
    /// The option the pointer is over, whose preview the pane shows.
    pub(crate) preview: Option<usize>,
    /// What holds the card's option keys back while the person types.
    pub(crate) guard: KeyGuard,
    // slopty: the preview pane's scroll, which its edge fades read.
    pub(crate) preview_scroll: gpui::ScrollHandle,
    // slopty: the proposed change's scroll, likewise.
    pub(crate) proposed_scroll: gpui::ScrollHandle,
}

impl Composer {
    /// Start the card over when the request it shows changes.
    fn card_for(&mut self, request_id: &str) {
        if self.card.request.as_deref() != Some(request_id) {
            let scopes = std::mem::take(&mut self.card.scopes);
            let watch = self.card.scopes_watch.take();
            let guard = self.card.guard;
            self.card = CardState { request: Some(request_id.to_owned()), scopes, scopes_watch: watch, guard, ..CardState::default() };
        }
    }

    /// Whether a text field the card shows has the keyboard: its answer box, a form's field, a
    /// note or a rule. What is typed there is the field's: no key answers the card from it.
    pub(super) fn field_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        let answer_box = self
            .wizard
            .as_ref()
            .is_some_and(|w| !w.folded && w.current().is_some_and(|q| takes_words(q, w.revising)));
        (answer_box && self.input.read(cx).focus_handle.is_focused(window)) || self.card_field_focused(window, cx)
    }

    /// Whether one of the card's own fields (a form's, a note's, a rule's) has the keyboard.
    pub(super) fn card_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.card
            .inputs
            .values()
            .chain(self.card.rule.as_ref().map(|rule| &rule.input))
            .any(|input| input.read(cx).focus_handle.is_focused(window))
    }

    /// A form field's input, by the field's name.
    #[cfg(test)]
    pub(crate) fn card_input(&self, name: &str) -> Option<Entity<ComposerInput>> {
        self.card.inputs.get(name).cloned()
    }

    /// The answers a card puts in its footer, after any Skip: a form's own answers (Decline,
    /// Cancel) and its Submit, or a dialog's Dismiss and OK. Every card's answers stand at the
    /// footer's right, the one that goes ahead last.
    pub(super) fn card_footer(&self, question: &UserInputQuestion, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if is_form(question) {
            let mut buttons: Vec<AnyElement> = question
                .options
                .iter()
                .enumerate()
                .map(|(ix, label)| {
                    let id = SharedString::from(format!("wizard-form-answer-{ix}"));
                    crate::surface_chrome::focus_ring(
                        crate::popover::btn_ghost(theme, label, id.clone())
                            .id(id)
                            .tab_index(0)
                            .px(px(12.0))
                            .py(px(6.0)),
                        theme,
                    )
                    .role(Role::Button)
                    .aria_label(SharedString::from(label.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| this.wizard_select(ix, cx)))
                    .into_any_element()
                })
                .collect();
            buttons.push(
                card_button(crate::popover::btn_primary(theme, "Submit"), "wizard-form-submit", theme)
                    .role(Role::Button)
                    .aria_label("Submit form")
                    .px(px(16.0))
                    .on_click(cx.listener(|this, _, _, cx| this.send_form(cx)))
                    .into_any_element(),
            );
            return buttons;
        }
        if takes_no_words(question) {
            return vec![
                card_button(crate::popover::btn_ghost(theme, "Dismiss", "wizard-dialog-dismiss"), "wizard-dialog-dismiss", theme)
                    .role(Role::Button)
                    .aria_label("Dismiss")
                    .on_click(cx.listener(|this, _, _, cx| this.answer_page_with(CANCEL.to_owned(), cx)))
                    .into_any_element(),
                card_button(crate::popover::btn_primary(theme, "OK"), "wizard-dialog-ok", theme)
                    .role(Role::Button)
                    .aria_label("OK")
                    .px(px(16.0))
                    .on_click(cx.listener(|this, _, _, cx| this.answer_page_with("{}".to_owned(), cx)))
                    .into_any_element(),
            ];
        }
        Vec::new()
    }

    /// Answer the open request's page with `label` (a form's object, a dialog's OK or dismiss).
    pub(super) fn answer_page_with(&mut self, label: String, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_ref() else { return };
        let page = wizard.page;
        let mut answers = wizard.answers();
        if let Some(answer) = answers.get_mut(page) {
            answer.labels = vec![label];
        }
        self.wizard_finish(answers, cx);
    }

    /// Send the open form as filled in, or say what it still needs.
    pub(super) fn send_form(&mut self, cx: &mut Context<Self>) {
        let Some(question) = self.wizard.as_ref().and_then(Wizard::current) else { return };
        let fields = question.schema_json.as_deref().map(form_fields).unwrap_or_default();
        let texts: HashMap<String, String> = self
            .card
            .inputs
            .iter()
            .map(|(name, input)| (name.clone(), input.read(cx).text().to_owned()))
            .collect();
        match form_value(&fields, &texts, &self.card.picks) {
            Ok(value) => self.answer_page_with(value.to_string(), cx),
            Err(why) => {
                self.card.error = Some(why);
                cx.notify();
            }
        }
    }

    /// Start or stop sending words instead of a choice.
    pub(super) fn set_revising(&mut self, on: bool, cx: &mut Context<Self>) {
        self.revise(on, None, cx);
    }

    /// Start sending words with the allow at option `allow` ("Allow, and say what's next…").
    pub(super) fn set_allowing(&mut self, allow: usize, cx: &mut Context<Self>) {
        self.revise(true, Some(allow), cx);
    }

    /// Edit the plan in the composer, from `plan`, to approve it as edited with the allow at
    /// `allow` (the TUI's Ctrl+G).
    pub(super) fn set_editing_plan(&mut self, allow: usize, plan: String, cx: &mut Context<Self>) {
        self.revise(true, Some(allow), cx);
        self.input.update(cx, |input, cx| {
            input.set_text(plan, cx);
            input.set_placeholder("The plan as it should be, then send to approve it", cx);
        });
    }

    /// Start or stop sending words: instead of a choice, or with the allow at `allow`.
    fn revise(&mut self, on: bool, allow: Option<usize>, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_mut() else { return };
        if wizard.revising == on && wizard.revise_allow == allow {
            return;
        }
        wizard.revising = on;
        wizard.revise_allow = allow.filter(|_| on);
        let plan = wizard.current().is_some_and(|q| q.kind == PLAN || q.header.eq_ignore_ascii_case("plan"));
        self.input.update(cx, |input, cx| {
            input.set_text("", cx);
            input.set_placeholder(
                if !on {
                    "Type your own answer, or pick an option above"
                } else if allow.is_some() {
                    "What should it do next?"
                } else if plan {
                    "What should change in the plan?"
                } else {
                    "What should it do instead?"
                },
                cx,
            );
        });
        self.focus_pending = on;
        cx.notify();
    }

    /// Back from a revision to the choices.
    pub(super) fn card_escape_revision(&mut self, cx: &mut Context<Self>) {
        self.set_revising(false, cx);
    }

    /// Whether Escape went to the card: it leaves "Revise…" or the rule's edit.
    pub(super) fn card_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.card.rule.take().is_some() {
            cx.notify();
            return true;
        }
        if self.wizard.as_ref().is_some_and(|w| w.revising) {
            self.set_revising(false, cx);
            return true;
        }
        false
    }

    /// Open the rule's edit, and learn the scopes the agent writes rules to.
    fn edit_rule(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.wizard.as_ref().and_then(Wizard::current).and_then(|q| q.rule.clone()) else {
            return;
        };
        let input = cx.new(|cx| {
            let mut input = ComposerInput::new("Rule, like Bash(cargo test:*)", cx).with_single_line();
            input.mono = true;
            input.set_text(offer.rule.clone(), cx);
            input
        });
        self.card.rule = Some(RuleEdit { input, scope: offer.scope.unwrap_or_else(|| "local".to_owned()) });
        self.watch_rule_scopes(cx);
        cx.notify();
    }

    fn watch_rule_scopes(&mut self, cx: &mut Context<Self>) {
        if self.card.scopes_watch.is_some() {
            return;
        }
        let (Some(chat), Some(engine)) = (self.dock.chat.clone(), self.state.read(cx).engine().cloned()) else {
            return;
        };
        self.card.scopes_watch = Some(cx.spawn(async move |this, cx| {
            let Ok(mut rx) =
                engine.client().subscribe_checked(calls::WATCH_RULES, serde_json::json!({ "chatId": chat })).await
            else {
                return;
            };
            while let Some(value) = rx.recv().await {
                let Ok(rules) = serde_json::from_value::<PermissionRules>(value) else { continue };
                if this
                    .update(cx, |this, cx| {
                        this.card.scopes = rules.scopes;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Allow the approval once and add its rule as edited (`slopty.AllowWithRule`).
    fn allow_with_rule(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.card.rule.as_ref() else { return };
        let rule = edit.input.read(cx).text().trim().to_owned();
        if rule.is_empty() {
            self.card.error = Some("Write the rule to add".to_owned());
            cx.notify();
            return;
        }
        let scope = edit.scope.clone();
        let (Some(chat), Some(engine)) = (self.dock.chat.clone(), self.state.read(cx).engine().cloned()) else {
            return;
        };
        let Some(wizard) = self.wizard.take() else { return };
        self.card.rule = None;
        self.advance_task = None;
        let request_id = wizard.request_id.clone();
        self.answered_requests.insert(request_id.clone());
        let draft = self.drafts.get(&self.current_key).cloned().unwrap_or_default();
        self.input.update(cx, |input, cx| {
            input.set_text(draft, cx);
            input.set_placeholder("Do anything…", cx);
            input.set_key_context(MESSAGE_COMPOSER_CONTEXT, cx);
        });
        let params = serde_json::json!({ "chatId": chat, "requestId": request_id, "rule": rule, "scope": scope });
        cx.spawn(async move |this, cx| {
            if let Err(err) = engine.client().call(calls::ALLOW_WITH_RULE, params).await {
                this.update(cx, |this, cx| {
                    this.failure = Some(format!("Couldn't allow with the rule: {err}").into());
                    this.failure_key = Some(chat);
                    // It never took: the request shows again.
                    this.answered_requests.remove(&request_id);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    /// Why the agent asks, in a quiet line under the question: the rule, the hook or the check
    /// that wanted the person's word, in the danger colour when the auto-mode classifier
    /// escalated it.
    pub(super) fn card_reason(question: &UserInputQuestion, theme: &Theme) -> Option<AnyElement> {
        let reason = question.reason.as_deref().map(str::trim).filter(|r| !r.is_empty())?;
        let words = reason.to_owned();
        Some(
            div()
                .id("wizard-reason")
                .role(Role::Note)
                .aria_label(SharedString::from(words.clone()))
                .mt(px(6.0))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(if classifier_escalated(reason) { theme.danger } else { theme.text_muted })
                .child(SharedString::from(words))
                .into_any_element(),
        )
    }

    /// The pane beside a question's options that shows what the option at `at` would look like
    /// (the TUI's preview pane), when any option has a preview.
    pub(super) fn render_previews(
        question: &UserInputQuestion,
        at: usize,
        scroll: &gpui::ScrollHandle,
        theme: &Theme,
    ) -> Option<AnyElement> {
        if question.option_previews.iter().all(|p| p.trim().is_empty()) {
            return None;
        }
        let shown = question.option_previews.get(at).map(|p| p.trim()).filter(|p| !p.is_empty());
        let label = question.options.get(at).cloned().unwrap_or_default();
        // slopty: the pane's words fade at the edges they scroll past, inside its box.
        let words = div()
            .id("wizard-preview")
            .role(Role::Region)
            .aria_label(SharedString::from(format!("Preview of {label}")))
            .max_h(px(280.0))
            .overflow_y_scroll()
            .track_scroll(scroll)
            .px(px(12.0))
            .py(px(10.0))
            .font_family(theme.font_mono.clone())
            .text_size(crate::typography::ui_rems(11.5))
            .line_height(px(17.0))
            .text_color(if shown.is_some() { theme.text } else { theme.text_muted })
            .child(SharedString::from(shown.map_or_else(|| "No preview available".to_owned(), str::to_owned)));
        Some(
            div()
                .flex_1()
                .min_w_0()
                .rounded(px(12.0))
                .bg(crate::theme::ink(0.04))
                .child(crate::popover::faded_menu_list(scroll, words))
                .into_any_element(),
        )
    }

    /// The header's chips: who asks, and when the agent stops waiting.
    pub(super) fn card_header_chips(&mut self, question: &UserInputQuestion, date: chrono::DateTime<chrono::Utc>, theme: &Theme) -> Vec<AnyElement> {
        let mut chips = Vec::new();
        if let Some(child) = question.sent_by.as_deref().filter(|c| !c.trim().is_empty()) {
            let words = format!("Sent by {child}");
            chips.push(
                div()
                    .id("wizard-sent-by")
                    .role(Role::Note)
                    .aria_label(SharedString::from(words.clone()))
                    .h(px(20.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .rounded(px(6.0))
                    .bg(crate::theme::ink(0.06))
                    .text_size(crate::typography::ui_rems(10.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(crate::icons::icon(crate::icons::BOT).size(px(11.0)).text_color(theme.text_muted))
                    .child(SharedString::from(words))
                    .into_any_element(),
            );
        }
        // slopty: Claude Code's `requires_user_interaction`: only a click or a key here answers
        // it, never a note's button, an API call or another agent, and the card says so.
        if question.needs_presence {
            let words = NEEDS_PRESENCE;
            chips.push(
                div()
                    .id("wizard-needs-presence")
                    .role(Role::Note)
                    .aria_label(SharedString::from(words))
                    .aria_description(NEEDS_PRESENCE_WHY)
                    .tooltip(crate::settings::widgets::text_tooltip(NEEDS_PRESENCE_WHY))
                    .h(px(20.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .rounded(px(6.0))
                    .bg(crate::theme::ink(0.06))
                    .text_size(crate::typography::ui_rems(10.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(crate::icons::icon(crate::icons::LOCK).size(px(11.0)).text_color(theme.text_muted))
                    .child(SharedString::from(words))
                    .into_any_element(),
            );
        }
        let now = date.timestamp_millis();
        if let Some((until, words)) = question.until_ms.and_then(|until| Some((until, countdown_words(until, now)?))) {
            let urgent = until - now < COUNTDOWN_URGENT_MS;
            chips.push(
                div()
                    .id("wizard-countdown")
                    .role(Role::Timer)
                    .aria_label(SharedString::from(words.clone()))
                    .ml_auto()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(if urgent { theme.warning } else { theme.text_faint })
                    .child(SharedString::from(words))
                    .into_any_element(),
            );
        }
        chips
    }

    /// What the card draws under its options, by the request's kind.
    pub(super) fn render_card_extras(
        &mut self,
        request_id: &str,
        question: &UserInputQuestion,
        plan: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.card_for(request_id);
        let mut extras = Vec::new();
        let revising = self.wizard.as_ref().is_some_and(|w| w.revising);
        if let Some(offer) = question.rule.clone().filter(|_| !revising) {
            extras.push(self.render_rule(&offer, theme, cx));
        }
        if question.kind == ELICITATION {
            if let Some(url) = question.url.clone() {
                extras.push(self.render_open_page(&url, theme));
            }
            let fields = question.schema_json.as_deref().map(form_fields).unwrap_or_default();
            if !fields.is_empty() {
                extras.push(self.render_form(&fields, theme, cx));
            }
        }
        if question.kind.starts_with(DIALOG) {
            let shown = format!("{}\n{}", question.header, question.question);
            let facts = question.schema_json.as_deref().map(|p| payload_facts(p, &shown)).unwrap_or_default();
            if !facts.is_empty() {
                extras.push(render_facts(&facts, theme));
            }
        }
        // slopty: notes on a design whose options have previews (the TUI's "Add notes on this
        // design…"), which go with the answer.
        if !revising && !question.multi_select && question.option_previews.iter().any(|p| !p.trim().is_empty()) {
            extras.push(self.render_notes(&question.id, theme, cx));
        }
        let revise = revise_words(question, plan).filter(|_| !revising);
        let allow = allow_words(question).zip(question.allow_option).filter(|_| !revising);
        // slopty: "Edit plan…", the TUI's Ctrl+G: the plan in the composer, approved as edited.
        let edit = question.edit_from.clone().zip(question.allow_option).filter(|_| !revising);
        // slopty: "Chat about this", beside an agent's questions.
        let aside = question.aside.clone().filter(|_| !revising);
        if revise.is_some() || allow.is_some() || edit.is_some() || aside.is_some() {
            let link = |id: &'static str, words: SharedString| card_link(id, words, theme.text_muted, theme);
            extras.push(
                div()
                    .mt(px(8.0))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(12.0))
                    .when_some(allow, |row, (words, ix)| {
                        // slopty: the TUI's "Yes, and tell Claude what to do next".
                        row.child(link("wizard-allow-say", words.into()).on_click(cx.listener(move |this, _, _, cx| this.set_allowing(ix, cx))))
                    })
                    .when_some(edit, |row, (plan, ix)| {
                        row.child(link("wizard-edit-plan", "Edit plan…".into()).on_click(cx.listener(move |this, _, _, cx| this.set_editing_plan(ix, plan.clone(), cx))))
                    })
                    .when_some(aside, |row, label| {
                        row.child(link("wizard-aside", label.clone().into()).on_click(cx.listener(move |this, _, _, cx| {
                            let answer = UserInputAnswer { question_id: ASIDE_ID.to_owned(), labels: vec![label.clone()], notes: None };
                            this.wizard_finish(vec![answer], cx);
                        })))
                    })
                    .when_some(revise, |row, words| {
                        row.child(link("wizard-revise", words.into()).on_click(cx.listener(|this, _, _, cx| this.set_revising(true, cx))))
                    })
                    .into_any_element(),
            );
        }
        if let Some(error) = self.card.error.clone() {
            extras.push(
                div()
                    .id("wizard-card-error")
                    .role(Role::Alert)
                    .aria_label(SharedString::from(error.clone()))
                    .mt(px(8.0))
                    .px(px(4.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.danger)
                    .child(SharedString::from(error))
                    .into_any_element(),
            );
        }
        extras
    }

    /// "Notes:" and a box for the person's notes on the question `id`.
    fn render_notes(&mut self, id: &str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let input = self
            .card
            .inputs
            .entry(format!("{NOTES}{id}"))
            .or_insert_with(|| {
                cx.new(|cx| {
                    let mut input = ComposerInput::new("", cx).with_single_line();
                    input.set_placeholder("Add notes on this design…", cx);
                    input
                })
            })
            .clone();
        div()
            .id("wizard-notes")
            .role(Role::Group)
            .aria_label("Notes")
            .mt(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child("Notes:"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(28.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(crate::theme::hairline(0.1))
                    .bg(crate::theme::ink(0.03))
                    .child(input),
            )
            .into_any_element()
    }

    /// `answers` with the card's notes on each question beside its answer.
    pub(super) fn with_card_notes(&self, mut answers: Vec<UserInputAnswer>, cx: &App) -> Vec<UserInputAnswer> {
        for answer in &mut answers {
            let notes = self
                .card
                .inputs
                .get(&format!("{NOTES}{}", answer.question_id))
                .map(|input| input.read(cx).text().trim().to_owned())
                .filter(|notes| !notes.is_empty());
            if notes.is_some() {
                answer.notes = notes;
            }
        }
        answers
    }

    /// "Always allows `Bash(cargo test:*)` · Edit rule…", or the rule's edit: the rule in mono,
    /// where it goes, and Allow with rule.
    fn render_rule(&mut self, offer: &slopty_view_proto::RuleOffer, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(edit) = self.card.rule.as_ref() else {
            let words = format!("Always allows {}", offer.rule);
            return div()
                .id("wizard-rule")
                .role(Role::Group)
                .aria_label(SharedString::from(words))
                .mt(px(8.0))
                .px(px(4.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child("Always allows")
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.font_mono.clone())
                        .text_color(theme.text)
                        .child(SharedString::from(offer.rule.clone())),
                )
                .child(
                    card_link("wizard-rule-edit", "Edit rule…".into(), theme.accent, theme)
                        .aria_label("Edit rule")
                        .text_size(crate::typography::ui_rems(12.0))
                        .flex_none()
                        .on_click(cx.listener(|this, _, _, cx| this.edit_rule(cx))),
                )
                .into_any_element();
        };
        let scopes: Vec<(String, String)> = if self.card.scopes.is_empty() {
            ["local", "project", "user"].iter().map(|id| ((*id).to_owned(), scope_words(id))).collect()
        } else {
            self.card.scopes.iter().map(|s| (s.id.clone(), s.label.clone())).collect()
        };
        let current = edit.scope.clone();
        div()
            .id("wizard-rule-editor")
            .role(Role::Group)
            .aria_label("Allow with rule")
            .mt(px(10.0))
            .p(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .rounded(px(crate::transcript::TRANSCRIPT_CARD_RADIUS))
            .border_1()
            .border_color(theme.border)
            .bg(crate::theme::ink(0.02))
            .child(
                div()
                    .h(px(30.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(crate::theme::hairline(0.1))
                    .bg(crate::theme::ink(0.03))
                    .child(edit.input.clone()),
            )
            .child(
                div()
                    .id("wizard-rule-scope")
                    .role(Role::RadioGroup)
                    .aria_label("Where the rule goes")
                    .flex()
                    .flex_wrap()
                    .gap(px(4.0))
                    .children(scopes.into_iter().enumerate().map(|(ix, (id, label))| {
                        let selected = id == current;
                        div()
                            .id(("wizard-rule-scope-option", ix))
                            .role(Role::RadioButton)
                            .aria_label(SharedString::from(label.clone()))
                            .aria_selected(selected)
                            .h(px(26.0))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .rounded(px(Theme::CONTROL_RADIUS))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(if selected { theme.text } else { theme.text_muted })
                            // slopty: the faded hover.
                            .bg(crate::motion::hover_blend(
                                &format!("wizard-rule-scope-option-{ix}"),
                                if selected { crate::theme::ink(0.09) } else { crate::theme::ink(0.03) },
                                if selected { crate::theme::ink(0.09) } else { crate::theme::ink(0.07) },
                            ))
                            .on_hover(crate::motion::hover_listener(format!("wizard-rule-scope-option-{ix}")))
                            .cursor_pointer()
                            // slopty: a tab stop with the shared ring; ↩ or Space picks it.
                            .tab_index(0)
                            .map(|el| crate::surface_chrome::focus_ring(el, theme))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(edit) = this.card.rule.as_mut() {
                                    edit.scope.clone_from(&id);
                                }
                                cx.notify();
                            }))
                            .child(SharedString::from(label))
                    })),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(6.0))
                    .child(
                        card_button(crate::popover::btn_ghost(theme, "Cancel", "wizard-rule-cancel"), "wizard-rule-cancel", theme)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.card.rule = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        card_button(crate::popover::btn_primary(theme, "Allow with rule"), "wizard-allow-with-rule", theme)
                            .role(Role::Button)
                            .aria_label("Allow with rule")
                            .on_click(cx.listener(|this, _, _, cx| this.allow_with_rule(cx))),
                    ),
            )
            .into_any_element()
    }

    /// A form filled in elsewhere: the page to open; Done once filled in.
    fn render_open_page(&self, url: &str, theme: &Theme) -> AnyElement {
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| url.to_owned());
        // slopty: a URL the card cannot show as it would open, or too long for a browser, is
        // not opened with a click, as the TUI's card refuses it.
        let caution = url_caution(url);
        let opens = (url.starts_with("https://") || url.starts_with("http://")) && caution.is_none();
        let shown = url.to_owned();
        let url = url.to_owned();
        let page = div()
            .id("wizard-open-page")
            .role(Role::Link)
            .aria_label(SharedString::from(format!("Open {host}")))
            .mt(px(8.0))
            .px(px(4.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(crate::typography::ui_rems(12.5))
            .text_color(theme.accent)
            // slopty: a tab stop with the shared ring when it opens.
            .when(opens, |el| {
                crate::surface_chrome::focus_ring(el.cursor_pointer().tab_index(0).rounded(px(Theme::CONTROL_RADIUS)), theme)
                    .on_click(move |_, _, cx| cx.open_url(&url))
            })
            .child(crate::icons::icon(crate::icons::GLOBAL).size(px(13.0)).text_color(theme.accent))
            .child(SharedString::from(format!("Open {host}")))
            .child(div().text_color(theme.text_faint).child("· then Done"));
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(page)
            // slopty: the whole address, as it would open, under its host.
            .child(
                div()
                    .px(px(4.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .font_family(theme.font_mono.clone())
                    .child(SharedString::from(shown)),
            )
            .when_some(caution, |el, why| {
                el.child(
                    div()
                        .id("wizard-url-caution")
                        .role(Role::Alert)
                        .aria_label(why)
                        .px(px(4.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.warning)
                        .child(why),
                )
            })
            .into_any_element()
    }

    /// An MCP server's form, from its schema: a box per text or number, chips per choice.
    fn render_form(&mut self, fields: &[FormField], theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = Vec::with_capacity(fields.len());
        for (ix, field) in fields.iter().enumerate() {
            let label = if field.required { format!("{} *", field.title) } else { field.title.clone() };
            let control = match &field.kind {
                FieldKind::Text | FieldKind::Number { .. } => {
                    let input = self
                        .card
                        .inputs
                        .entry(field.name.clone())
                        .or_insert_with(|| {
                            let default = field.default.clone().unwrap_or_default();
                            cx.new(|cx| {
                                let mut input = ComposerInput::new("", cx).with_single_line();
                                input.set_text(default, cx);
                                input
                            })
                        })
                        .clone();
                    div()
                        .h(px(30.0))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(crate::theme::hairline(0.1))
                        .bg(crate::theme::ink(0.03))
                        .child(input)
                        .into_any_element()
                }
                FieldKind::Bool => self.render_chips(
                    ix,
                    field,
                    &[("true".to_owned(), "Yes".to_owned()), ("false".to_owned(), "No".to_owned())],
                    false,
                    theme,
                    cx,
                ),
                FieldKind::Choice(options) => self.render_chips(ix, field, options, false, theme, cx),
                FieldKind::Choices(options) => self.render_chips(ix, field, options, true, theme, cx),
            };
            rows.push(
                div()
                    .id(("wizard-form-field", ix))
                    .role(Role::Group)
                    .aria_label(SharedString::from(label.clone()))
                    .when_some(field.description.clone(), |el, d| el.aria_description(SharedString::from(d)))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(12.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(label)),
                    )
                    .when_some(field.description.clone(), |el, d| {
                        el.child(
                            div()
                                .text_size(crate::typography::ui_rems(11.5))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(d)),
                        )
                    })
                    .child(control)
                    .into_any_element(),
            );
        }
        div()
            .id("wizard-form")
            .role(Role::Form)
            .aria_label("Form")
            .mt(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .children(rows)
            .into_any_element()
    }

    fn render_chips(
        &self,
        field_ix: usize,
        field: &FormField,
        options: &[(String, String)],
        many: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let picked = self.card.picks.get(&field.name).cloned().unwrap_or_else(|| {
            field.default.clone().filter(|_| !many).into_iter().collect()
        });
        div()
            .flex()
            .flex_wrap()
            .gap(px(4.0))
            .children(options.iter().enumerate().map(|(ix, (value, label))| {
                let selected = picked.contains(value);
                let (name, value) = (field.name.clone(), value.clone());
                div()
                    .id(SharedString::from(format!("wizard-form-{field_ix}-{ix}")))
                    .role(if many { Role::CheckBox } else { Role::RadioButton })
                    .aria_label(SharedString::from(label.clone()))
                    .aria_selected(selected)
                    .h(px(26.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(if selected { theme.text } else { theme.text_muted })
                    // slopty: the faded hover.
                    .bg(crate::motion::hover_blend(
                        &format!("wizard-form-{field_ix}-{ix}"),
                        if selected { crate::theme::ink(0.09) } else { crate::theme::ink(0.03) },
                        if selected { crate::theme::ink(0.09) } else { crate::theme::ink(0.07) },
                    ))
                    .on_hover(crate::motion::hover_listener(format!("wizard-form-{field_ix}-{ix}")))
                    .cursor_pointer()
                    // slopty: a tab stop with the shared ring; ↩ or Space picks it.
                    .tab_index(0)
                    .map(|el| crate::surface_chrome::focus_ring(el, theme))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let picks = this.card.picks.entry(name.clone()).or_default();
                        match picks.iter().position(|p| *p == value) {
                            Some(at) if many => {
                                picks.remove(at);
                            }
                            _ if many => picks.push(value.clone()),
                            _ => *picks = vec![value.clone()],
                        }
                        this.card.error = None;
                        cx.notify();
                    }))
                    .child(SharedString::from(label.clone()))
            }))
            .into_any_element()
    }
}

/// A dialog's facts, by name.
fn render_facts(facts: &[(String, String)], theme: &Theme) -> AnyElement {
    div()
        .id("wizard-dialog-facts")
        .role(Role::List)
        .aria_label("Details")
        .mt(px(10.0))
        .px(px(4.0))
        .flex()
        .flex_col()
        .gap(px(3.0))
        .children(facts.iter().enumerate().map(|(ix, (name, words))| {
            div()
                .id(("wizard-dialog-fact", ix))
                .role(Role::ListItem)
                .aria_label(SharedString::from(format!("{name}: {words}")))
                .flex()
                .gap(px(8.0))
                .text_size(crate::typography::ui_rems(12.0))
                .child(div().flex_none().w(px(120.0)).truncate().text_color(theme.text_faint).child(SharedString::from(name.clone())))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(words.clone())),
                )
        }))
        .into_any_element()
}

/// slopty: a card's button (`popover::btn_ghost`, `btn_primary`) as a tab stop with the shared
/// focus ring.
pub(super) fn card_button(button: gpui::Div, id: &'static str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    crate::surface_chrome::focus_ring(button.id(id).tab_index(0).px(px(12.0)).py(px(6.0)), theme)
}

/// slopty: one of the card's word buttons ("Revise…", "Edit plan…", "Edit rule…"): a tab stop
/// with the shared focus ring, its words easing from `rest` to the text colour on hover.
fn card_link(id: &'static str, words: SharedString, rest: gpui::Hsla, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let fade: SharedString = format!("{id}-hover").into();
    let text = if rest == theme.accent { theme.accent.opacity(0.8) } else { theme.text };
    let link = div()
        .id(id)
        .role(Role::Button)
        .aria_label(words.clone())
        .px(px(4.0))
        .rounded(px(Theme::CONTROL_RADIUS))
        .text_size(crate::typography::ui_rems(12.5))
        .text_color(crate::motion::hover_blend(&fade, rest, text))
        .on_hover(crate::motion::hover_listener(fade))
        .cursor_pointer()
        .tab_index(0)
        .child(words);
    crate::surface_chrome::focus_ring(link, theme)
}

/// What a rewind's answer offers to undo, whenever it has an undo (a branch to delete, a folder
/// to restore): a toast with "Undo rewind" (`slopty.UndoRewind`), which puts both back and
/// shows the chat it answers with. The worker's refusal is an error toast, and a rewind with no
/// undo says why (`noUndo`).
pub fn offer_undo_rewind(answer: &Value, state: Entity<AppState>, cx: &mut App) {
    let Some(undo) = answer.get("undo").filter(|u| u.is_object()).cloned() else {
        if let Some(why) = answer.get("noUndo").and_then(Value::as_str) {
            crate::toast::info(cx, why.to_owned());
        }
        return;
    };
    let branched = undo.get("branchChatId").and_then(Value::as_str).is_some_and(|b| !b.is_empty());
    let words = if branched { "Rewound into a new chat" } else { "Rewound the code" };
    crate::toast::offer(cx, words, "Undo rewind", Duration::from_secs(10), move |cx| {
        let Some(engine) = state.read(cx).engine().cloned() else { return };
        let (undo, state) = (undo.clone(), state.clone());
        cx.spawn(async move |cx| {
            match engine.client().call(calls::UNDO_REWIND, serde_json::json!({ "undo": undo })).await {
                Ok(value) => {
                    let chat = value.get("chatId").and_then(Value::as_str).map(str::to_owned);
                    cx.update(|cx| {
                        if let Some(chat) = chat {
                            state.update(cx, |state, cx| state.select_chat(Some(chat), cx));
                        }
                    });
                }
                Err(err) => {
                    cx.update(|cx| crate::toast::error(cx, format!("Couldn't undo the rewind: {err}")));
                }
            }
        })
        .detach();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A form's fields come in the server's order with their kinds, and its answer is the
    /// object of what was filled in; a missing required field or a word for a number is said.
    #[test]
    fn a_form_reads_its_schema_and_answers_an_object() {
        let schema = r#"{"type":"object","properties":{
            "name":{"type":"string","title":"Name"},
            "count":{"type":"integer","description":"How many"},
            "ok":{"type":"boolean","default":true},
            "tone":{"type":"string","enum":["a","b"],"enumNames":["Calm","Bold"]},
            "tags":{"type":"array","items":{"anyOf":[{"const":"x","title":"Ex"},{"const":"y"}]}}
        },"required":["name"],"x-property-order":["tone","name","count","ok","tags"]}"#;
        let fields = form_fields(schema);
        let names: Vec<_> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["tone", "name", "count", "ok", "tags"]);
        assert_eq!(fields[0].kind, FieldKind::Choice(vec![("a".into(), "Calm".into()), ("b".into(), "Bold".into())]));
        assert_eq!(fields[2].kind, FieldKind::Number { integer: true });
        assert_eq!(fields[3].default.as_deref(), Some("true"));
        assert_eq!(fields[4].kind, FieldKind::Choices(vec![("x".into(), "Ex".into()), ("y".into(), "y".into())]));
        assert!(fields[1].required);

        let mut texts = HashMap::new();
        let mut picks = HashMap::new();
        assert_eq!(form_value(&fields, &texts, &picks), Err("Fill in Name".to_owned()));
        texts.insert("name".to_owned(), " Ada ".to_owned());
        texts.insert("count".to_owned(), "three".to_owned());
        assert_eq!(form_value(&fields, &texts, &picks), Err("count takes a whole number".to_owned()));
        texts.insert("count".to_owned(), "3".to_owned());
        picks.insert("ok".to_owned(), vec!["false".to_owned()]);
        picks.insert("tags".to_owned(), vec!["y".to_owned(), "x".to_owned()]);
        let value = form_value(&fields, &texts, &picks).expect("filled in");
        assert_eq!(value, serde_json::json!({ "name": "Ada", "count": 3, "ok": false, "tags": ["y", "x"] }));
        assert!(form_fields("not json").is_empty());

        // The schema's limits are said the TUI's way.
        let limited = form_fields(
            r#"{"properties":{"mail":{"type":"string","format":"email"},"code":{"type":"string","minLength":4},"n":{"type":"number","maximum":10}}}"#,
        );
        let only = |name: &str, text: &str| {
            HashMap::from([(name.to_owned(), text.to_owned())])
        };
        let none = HashMap::new();
        assert_eq!(
            form_value(&limited, &only("mail", "not an address"), &none),
            Err("mail must be a valid email address, e.g. user@example.com".to_owned())
        );
        assert_eq!(form_value(&limited, &only("code", "abc"), &none), Err("code must be at least 4 characters".to_owned()));
        assert_eq!(form_value(&limited, &only("n", "12.5"), &none), Err("n must be at most 10".to_owned()));
        assert_eq!(form_value(&limited, &only("mail", "a@b.co"), &none), Ok(serde_json::json!({ "mail": "a@b.co" })));
    }

    /// A dialog's payload lists what its card does not already say, by name.
    #[test]
    fn a_dialog_lists_its_payload_beyond_its_title() {
        let payload = r#"{"condition":"tests pass","fallbackModel":"opus","retries":2,"options":[],"nested":{"a":1}}"#;
        let facts = payload_facts(payload, "Work towards a goal: tests pass");
        assert_eq!(facts, [("Fallback model".to_owned(), "opus".to_owned()), ("Retries".to_owned(), "2".to_owned())]);
        assert!(payload_facts("[]", "").is_empty());
    }

    /// A classifier's escalation reads as one; a rule's or a hook's does not.
    #[test]
    fn a_classifier_escalation_stands_out() {
        assert!(classifier_escalated("Auto mode classifier requires confirmation for this command."));
        assert!(classifier_escalated("Classifier risky-ops requires confirmation for this tool."));
        assert!(!classifier_escalated("Permission rule Bash(rm:*) requires confirmation"));
    }

    /// An address the view model had to escape, or one that breaks a line, does not open with
    /// a click; a plain one does.
    #[test]
    fn a_url_with_hidden_characters_does_not_open() {
        assert_eq!(url_caution("https://example.com/a?b=c"), None);
        assert!(url_caution("https://example.com/[U+202E]gpj.exe").is_some(), "escaped by the view model");
        assert!(url_caution("https://example.com/\nnext").is_some());
        assert!(url_caution(&format!("https://example.com/{}", "%".repeat(2_000))).is_some(), "too long");
    }

    /// The countdown says how long is left, rounded up, and nothing once past.
    #[test]
    fn the_countdown_reads_what_is_left() {
        assert_eq!(countdown_words(42_500, 1_000).as_deref(), Some("Declines by itself in 42s"));
        assert_eq!(countdown_words(1_000, 1_000), None);
    }

    /// "Revise…" stands on a plan, "Say what to do instead…" on an approval with a deny, and
    /// nothing where words could not go.
    #[test]
    fn revise_stands_where_words_can_go() {
        let mut question = UserInputQuestion {
            choices_only: true,
            deny_option: Some(1),
            kind: "plan".into(),
            ..UserInputQuestion::default()
        };
        assert_eq!(revise_words(&question, false), Some("Revise…"));
        question.kind = "approval".into();
        assert_eq!(revise_words(&question, false), Some("Say what to do instead…"));
        question.kind = "dialog:goal_proposal".into();
        assert_eq!(revise_words(&question, false), None);
        question.kind = "approval".into();
        question.deny_option = None;
        assert_eq!(revise_words(&question, false), None);
        let dialog = UserInputQuestion { kind: "dialog:new_thing".into(), multiline: true, ..UserInputQuestion::default() };
        assert!(takes_no_words(&dialog));
    }

    /// "Allow, and say what's next…" stands on an approval whose agent takes words with its
    /// allow, and nowhere else.
    #[test]
    fn allow_with_words_stands_where_the_agent_takes_them() {
        let mut question = UserInputQuestion {
            choices_only: true,
            allow_option: Some(0),
            kind: "approval".into(),
            ..UserInputQuestion::default()
        };
        assert_eq!(allow_words(&question), Some("Allow, and say what's next…"));
        question.kind = "plan".into();
        assert_eq!(allow_words(&question), None);
        question.kind = "approval".into();
        question.allow_option = None;
        assert_eq!(allow_words(&question), None);
    }
}
