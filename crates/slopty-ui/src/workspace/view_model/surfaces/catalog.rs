//! Claude's models as the picker offers them: what Claude Code's own `/model` menu takes, named
//! from T3 Code's model manifest (`assets/t3code-model-manifest.json`, MIT,
//! `THIRD_PARTY_NOTICES.md`).
//!
//! The menu is Claude Code's, as Slopty's mod reads it from the running Claude Code (the
//! `/config` menu's `model` row): the models this account and its managed settings allow,
//! the same the terminal's `/model` offers, by alias (`default`, `sonnet`, `opus`). The
//! manifest names each model by its id, with its name, the aliases it answers to, whether it
//! is current, the efforts it takes, and the oldest Claude Code that runs it; T3 Code keeps it
//! up to date. Each alias of the menu is offered as the model it answers for, by id, so what a
//! chat starts on is exact; an entry the manifest does not know is offered as the menu says it.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;
use slopty_view_proto::{Model, ReasoningLevel};

/// The manifest's driver for Claude Code.
const CLAUDE: &str = "claudeAgent";

#[derive(Debug, Deserialize)]
struct Manifest {
    providers: HashMap<String, Provider>,
}

#[derive(Debug, Deserialize)]
struct Provider {
    #[serde(default)]
    profiles: HashMap<String, Profile>,
    #[serde(default)]
    models: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Profile {
    #[serde(default)]
    capabilities: Capabilities,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    #[serde(default)]
    option_descriptors: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
struct Descriptor {
    id: String,
    #[serde(default)]
    options: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    id: String,
}

#[derive(Debug, Deserialize)]
struct Entry {
    slug: String,
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    status: String,
    profile: Option<String>,
    #[serde(default)]
    adapter: Adapter,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Adapter {
    claude_code: Option<Runs>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Runs {
    min_version: Option<String>,
}

/// The manifest, read once; `None` were it ever not to read.
static MANIFEST: LazyLock<Option<Manifest>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../../assets/t3code-model-manifest.json"))
        .inspect_err(|e| tracing::warn!(%e, "the model manifest does not read"))
        .ok()
});

/// Claude's entries in the manifest.
fn claude() -> Option<&'static Provider> {
    MANIFEST.as_ref()?.providers.get(CLAUDE)
}

/// A version's numbers (`2.1.296` is 2, 1, 296); what does not read is 0.
fn numbers(version: &str) -> Vec<u64> {
    version
        .trim()
        .split(['.', '-', '+', ' '])
        .take(3)
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

impl Entry {
    /// Whether Claude Code `version` runs it; any does when the manifest names no oldest.
    fn runs_on(&self, version: &str) -> bool {
        let oldest = self.adapter.claude_code.as_ref().and_then(|r| r.min_version.as_deref());
        oldest.is_none_or(|oldest| numbers(version) >= numbers(oldest))
    }

    /// Whether `id` names it: its id, or an alias it answers to.
    fn named(&self, id: &str) -> bool {
        self.slug.eq_ignore_ascii_case(id)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(id))
    }

    /// It as the picker's row: its name without "Claude", the efforts its profile takes.
    fn row(&self, profiles: &HashMap<String, Profile>) -> Model {
        let efforts = self
            .profile
            .as_ref()
            .and_then(|profile| profiles.get(profile))
            .into_iter()
            .flat_map(|profile| &profile.capabilities.option_descriptors)
            .filter(|descriptor| descriptor.id == "effort")
            .flat_map(|descriptor| &descriptor.options)
            .filter_map(|choice| serde_json::from_value(serde_json::json!(choice.id)).ok())
            .collect::<Vec<ReasoningLevel>>();
        Model {
            id: self.slug.clone(),
            label: self.name.strip_prefix("Claude ").unwrap_or(&self.name).to_owned(),
            description: None,
            reasoning_levels: efforts,
            options: Vec::new(),
        }
    }
}

/// The current Claude models Claude Code `version` runs, in the manifest's order, and `also`
/// (a model a thread runs, by id or alias) when it is one of the older ones.
pub(super) fn claude_models(version: &str, also: Option<&str>) -> Vec<Model> {
    let Some(claude) = claude() else { return Vec::new() };
    let mut rows: Vec<Model> = claude
        .models
        .iter()
        .filter(|entry| entry.status == "current" && entry.runs_on(version))
        .map(|entry| entry.row(&claude.profiles))
        .collect();
    let older = also.and_then(|id| claude.models.iter().find(|entry| entry.named(id)));
    if let Some(entry) = older
        && !rows.iter().any(|row| row.id == entry.slug)
    {
        rows.push(entry.row(&claude.profiles));
    }
    rows
}

/// Claude Code's own menu (`menu`, by alias or id, as the running Claude Code offers it) as the
/// picker's rows, each alias as the model it answers for, and `also` (a model a thread runs)
/// where the menu has it not. With no menu yet, the current models Claude Code `version`
/// runs.
pub(super) fn claude_menu(version: &str, menu: &[String], also: Option<&str>) -> Vec<Model> {
    let Some(claude) = claude() else { return Vec::new() };
    if menu.is_empty() {
        return claude_models(version, also);
    }
    let mut rows: Vec<Model> = Vec::new();
    let mut add = |row: Model| {
        if !rows.iter().any(|r| r.id == row.id) {
            rows.push(row);
        }
    };
    for id in menu.iter().map(String::as_str).chain(also) {
        if id.eq_ignore_ascii_case("default") {
            continue;
        }
        let bare = id.trim_end_matches("[1m]");
        match claude.models.iter().find(|entry| entry.named(bare)) {
            Some(entry) => add(entry.row(&claude.profiles)),
            None => add(Model {
                id: id.to_owned(),
                label: id.to_owned(),
                description: None,
                reasoning_levels: Vec::new(),
                options: Vec::new(),
            }),
        }
    }
    rows
}

/// The id of the Claude model `id` names: its own, or the one its alias answers for.
pub(super) fn claude_id(id: &str) -> Option<&'static str> {
    let id = id.trim().trim_end_matches("[1m]");
    claude()?.models.iter().find(|entry| entry.named(id)).map(|entry| entry.slug.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_models_are_offered_by_id_where_claude_code_runs_them() {
        let ids: Vec<String> = claude_models("2.1.296", None).into_iter().map(|m| m.id).collect();
        for current in
            ["claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1", "claude-haiku-5-5"]
        {
            assert!(ids.iter().any(|id| id == current), "{current} in {ids:?}");
        }
        assert!(!ids.iter().any(|id| id == "claude-haiku-4-5"), "an older model is not offered");
        let older: Vec<String> = claude_models("2.1.280", None).into_iter().map(|m| m.id).collect();
        assert!(!older.iter().any(|id| id == "claude-haiku-5-5"), "too new for 2.1.280");
    }

    #[test]
    fn claude_codes_own_menu_is_offered_as_the_models_its_aliases_answer_for() {
        let menu: Vec<String> = ["default", "sonnet", "opus", "haiku", "sonnet[1m]", "opusplan"]
            .map(String::from)
            .into();
        let ids: Vec<String> =
            claude_menu("2.1.296", &menu, None).into_iter().map(|m| m.id).collect();
        assert_eq!(
            ids,
            ["claude-sonnet-5-5", "claude-opus-5-5", "claude-haiku-5-5", "opusplan"],
            "default is the account's, left to the agent; a 1M alias is its model"
        );
        let fallback = claude_menu("2.1.296", &[], None);
        assert!(fallback.iter().any(|m| m.id == "claude-fable-5-1"), "no menu yet: the current");
    }

    #[test]
    fn a_model_is_found_by_its_alias_and_named_without_claude() {
        assert_eq!(claude_id("opus"), Some("claude-opus-5-5"));
        assert_eq!(claude_id("sonnet[1m]"), Some("claude-sonnet-5-5"));
        assert_eq!(claude_id("claude-haiku-4-5-20251001"), Some("claude-haiku-4-5"));
        assert_eq!(claude_id("gpt-6"), None);
        let rows = claude_models("2.1.296", Some("claude-haiku-4-5"));
        let opus = rows.iter().find(|m| m.id == "claude-opus-5-5").expect("Opus 5.5");
        assert_eq!(opus.label, "Opus 5.5");
        assert!(opus.reasoning_levels.contains(&ReasoningLevel::XHigh));
        assert!(rows.iter().any(|m| m.id == "claude-haiku-4-5"), "the one a thread runs");
    }
}
