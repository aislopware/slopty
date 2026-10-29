//! Claude's models by id, as T3 Code's model manifest names them
//! (`assets/t3code-model-manifest.json`, MIT, `THIRD_PARTY_NOTICES.md`): a model Claude Code
//! says it runs by an alias (`opus`, `sonnet[1m]`) is shown as the model that alias answers
//! for. T3 Code keeps the manifest up to date.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

/// The manifest's driver for Claude Code.
const CLAUDE: &str = "claudeAgent";

#[derive(Debug, Deserialize)]
struct Manifest {
    providers: HashMap<String, Provider>,
}

#[derive(Debug, Deserialize)]
struct Provider {
    #[serde(default)]
    models: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    slug: String,
    #[serde(default)]
    aliases: Vec<String>,
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

impl Entry {
    /// Whether `id` names it: its id, or an alias it answers to.
    fn named(&self, id: &str) -> bool {
        self.slug.eq_ignore_ascii_case(id)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(id))
    }
}

/// The id of the Claude model `id` names: its own, or the one its alias answers for.
pub(super) fn claude_id(id: &str) -> Option<&'static str> {
    let id = id.trim().trim_end_matches("[1m]");
    claude()?.models.iter().find(|entry| entry.named(id)).map(|entry| entry.slug.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model is found by its id or an alias it answers to, a 1M alias as its model.
    #[test]
    fn a_model_is_found_by_its_alias() {
        assert_eq!(claude_id("opus"), Some("claude-opus-5-5"));
        assert_eq!(claude_id("sonnet[1m]"), Some("claude-sonnet-5-5"));
        assert_eq!(claude_id("claude-haiku-4-5-20251001"), Some("claude-haiku-4-5"));
        assert_eq!(claude_id("gpt-6"), None);
    }
}
